//! # pgconn
//!
//! 一个轻量级的同步 PostgreSQL 客户端库。
//!
//! - `new()` 按 host / port / db / user / password 配置连接参数
//! - 方法名遵循 SQL 语法：`create_table` / `select` / `insert` / `update` / `delete` ...
//! - 所有值参数均走参数化查询，标识符（表名/列名）自动加引号，防 SQL 注入
//!
//! # 示例
//!
//! ```no_run
//! use pgconn::PgClient;
//!
//! # fn main() -> Result<(), pgconn::PgError> {
//! let mut client = PgClient::new("localhost", 5432, "postgres", "secret", "mydb");
//! client.connect()?;
//!
//! client.create_table("users", &[
//!     ("id", "SERIAL PRIMARY KEY"),
//!     ("name", "TEXT NOT NULL"),
//!     ("age", "INT"),
//! ])?;
//!
//! client.insert("users", &[("name", &"Alice"), ("age", &30)])?;
//!
//! let rows = client.select("users", Some(&["id", "name"]), Some("age > $1"), &[&18], Some("id"), Some(10))?;
//! for row in rows {
//!     let name: String = row.get("name");
//!     println!("{name}");
//! }
//! # Ok(())
//! # }
//! ```

use postgres::{Client, Config, NoTls, Row, Transaction};
use std::time::Duration;
use thiserror::Error;

/// 统一错误类型。
#[derive(Debug, Error)]
pub enum PgError {
    /// 连接建立失败或查询执行失败。
    #[error("postgres error: {0}")]
    Postgres(#[from] postgres::Error),
    /// 连接尚未建立（未调用 `connect()`）。
    #[error("connection not established: call connect() first")]
    NotConnected,
    /// 连接配置或参数非法。
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

pub type PgResult<T> = Result<T, PgError>;

/// 连接配置。
#[derive(Debug, Clone)]
pub struct PgConfig {
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: Option<String>,
    application_name: String,
    connect_timeout_secs: u64,
}

impl PgConfig {
    fn validate(&self) -> PgResult<()> {
        if self.host.trim().is_empty() {
            return Err(PgError::InvalidArgument("host must not be empty".into()));
        }
        if self.dbname.trim().is_empty() {
            return Err(PgError::InvalidArgument("dbname must not be empty".into()));
        }
        if self.user.trim().is_empty() {
            return Err(PgError::InvalidArgument("user must not be empty".into()));
        }
        Ok(())
    }
}

impl std::fmt::Display for PgConfig {
    /// 输出 postgres:// 连接串（密码以 *** 脱敏）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auth = match &self.password {
            Some(_) => format!("{}:***", self.user),
            None => self.user.clone(),
        };
        write!(
            f,
            "postgres://{}@{}:{}/{}",
            auth, self.host, self.port, self.dbname
        )
    }
}

/// PostgreSQL 客户端。
///
/// # 示例
/// ```no_run
/// use pgconn::PgClient;
/// let mut client = PgClient::new("localhost", 5432, "postgres", "secret", "mydb");
/// client.connect().unwrap();
/// ```
pub struct PgClient {
    config: PgConfig,
    client: Option<Client>,
}

impl PgClient {
    /// 创建连接配置（此时并未真正连接，需再调用 `connect()`）。
    ///
    /// - `host`：主机地址，如 `"localhost"`
    /// - `port`：端口，通常 `5432`
    /// - `user` / `password`：认证信息
    /// - `dbname`：数据库名
    pub fn new(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: impl Into<String>,
        dbname: impl Into<String>,
    ) -> Self {
        Self {
            config: PgConfig {
                host: host.into(),
                port,
                dbname: dbname.into(),
                user: user.into(),
                password: Some(password.into()),
                application_name: "pgconn".into(),
                connect_timeout_secs: 10,
            },
            client: None,
        }
    }

    /// 从 DSN 字符串创建：`postgres://user:pass@host:port/dbname`
    pub fn from_dsn(dsn: &str) -> PgResult<Self> {
        let parsed = url::Url::parse(dsn)
            .map_err(|_| PgError::InvalidArgument(format!("invalid dsn: {dsn}")))?;
        Ok(Self {
            config: PgConfig {
                host: parsed.host_str().unwrap_or("localhost").to_string(),
                port: parsed.port().unwrap_or(5432),
                dbname: parsed.path().trim_start_matches('/').to_string(),
                user: parsed.username().to_string(),
                password: parsed.password().map(|p| p.to_string()),
                application_name: "pgconn".into(),
                connect_timeout_secs: 10,
            },
            client: None,
        })
    }

    /// 建立连接。
    pub fn connect(&mut self) -> PgResult<()> {
        self.config.validate()?;
        let mut cfg = Config::new();
        cfg.host(&self.config.host)
            .port(self.config.port)
            .dbname(&self.config.dbname)
            .user(&self.config.user)
            .application_name(&self.config.application_name)
            .connect_timeout(Duration::from_secs(self.config.connect_timeout_secs));
        if let Some(p) = &self.config.password {
            cfg.password(p);
        }
        self.client = Some(cfg.connect(NoTls).map_err(PgError::Postgres)?);
        Ok(())
    }

    /// 返回连接串（密码脱敏），便于日志输出。
    pub fn dsn_display(&self) -> String {
        self.config.to_string()
    }

    fn conn(&mut self) -> PgResult<&mut Client> {
        self.client.as_mut().ok_or(PgError::NotConnected)
    }

    /// 开启事务，drop（未显式 commit）时自动回滚。
    pub fn begin(&mut self) -> PgResult<PgTransaction<'_>> {
        let tx = self.conn()?.transaction().map_err(PgError::Postgres)?;
        Ok(PgTransaction { tx })
    }

    // ===================== DDL =====================

    /// 创建数据表。`columns` 为 (列名, 列定义) 列表，列定义原样写入（如
    /// `"SERIAL PRIMARY KEY"`、`"TEXT NOT NULL"`）。
    ///
    /// ```no_run
    /// # use pgconn::PgClient;
    /// # let mut client = PgClient::new("", 5432, "", "", "");
    /// client.create_table("users", &[("id", "SERIAL PRIMARY KEY"), ("name", "TEXT NOT NULL")])?;
    /// # Ok::<(), pgconn::PgError>(())
    /// ```
    pub fn create_table(&mut self, table: &str, columns: &[(&str, &str)]) -> PgResult<u64> {
        if columns.is_empty() {
            return Err(PgError::InvalidArgument(
                "create_table requires at least one column".into(),
            ));
        }
        let cols: Vec<String> = columns
            .iter()
            .map(|(name, ty)| format!("{} {}", quote_ident(name), ty))
            .collect();
        let sql = format!("CREATE TABLE {} ({})", quote_ident(table), cols.join(", "));
        self.execute(&sql, &[])
    }

    /// 删除数据表。`if_exists` 为 true 时生成 `DROP TABLE IF EXISTS`。
    pub fn drop_table(&mut self, table: &str, if_exists: bool) -> PgResult<u64> {
        let clause = if if_exists { "IF EXISTS " } else { "" };
        let sql = format!("DROP TABLE {clause}{}", quote_ident(table));
        self.execute(&sql, &[])
    }

    // ===================== DML =====================

    /// 查询数据。`columns` 为 None 时等价于 `SELECT *`；
    /// `where_clause` 中的 `$1/$2...` 占位符对应 `params`；
    /// `order_by` 传列名或排序子句（如 `"id DESC"`）；`limit` 为 None 时不限。
    pub fn select(
        &mut self,
        table: &str,
        columns: Option<&[&str]>,
        where_clause: Option<&str>,
        params: &[&(dyn postgres::types::ToSql + Sync)],
        order_by: Option<&str>,
        limit: Option<i64>,
    ) -> PgResult<Vec<Row>> {
        let cols = columns
            .map(|c| c.iter().map(|n| quote_ident(n)).collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| "*".to_string());
        let mut sql = format!("SELECT {cols} FROM {}", quote_ident(table));
        if let Some(w) = where_clause {
            sql.push_str(" WHERE ");
            sql.push_str(w);
        }
        if let Some(o) = order_by {
            sql.push_str(" ORDER BY ");
            sql.push_str(o);
        }
        if let Some(l) = limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        self.query(&sql, params)
    }

    /// 插入数据，返回受影响行数。
    ///
    /// ```no_run
    /// # use pgconn::PgClient;
    /// # let mut client = PgClient::new("", 5432, "", "", "");
    /// client.insert("users", &[("name", &"Alice"), ("age", &30)])?;
    /// # Ok::<(), pgconn::PgError>(())
    /// ```
    pub fn insert(
        &mut self,
        table: &str,
        values: &[(&str, &(dyn postgres::types::ToSql + Sync))],
    ) -> PgResult<u64> {
        if values.is_empty() {
            return Err(PgError::InvalidArgument(
                "insert requires at least one column".into(),
            ));
        }
        let cols: Vec<String> = values.iter().map(|(c, _)| quote_ident(c)).collect();
        let placeholders: Vec<String> = (1..=values.len()).map(|i| format!("${i}")).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            quote_ident(table),
            cols.join(", "),
            placeholders.join(", ")
        );
        let params: Vec<&(dyn postgres::types::ToSql + Sync)> =
            values.iter().map(|(_, v)| *v).collect();
        self.execute(&sql, &params)
    }

    /// 更新数据。`sets` 为 (列名, 新值)；`where_clause` 中的 `$1..$n` 对应
    /// `params`（内部自动平移编号）。返回受影响行数。
    pub fn update(
        &mut self,
        table: &str,
        sets: &[(&str, &(dyn postgres::types::ToSql + Sync))],
        where_clause: &str,
        params: &[&(dyn postgres::types::ToSql + Sync)],
    ) -> PgResult<u64> {
        if sets.is_empty() {
            return Err(PgError::InvalidArgument(
                "update requires at least one column".into(),
            ));
        }
        let offset = sets.len();
        let set_clause: Vec<String> = sets
            .iter()
            .enumerate()
            .map(|(i, (c, _))| format!("{} = ${}", quote_ident(c), i + 1))
            .collect();
        let where_clause = shift_placeholders(where_clause, offset)?;
        let sql = format!(
            "UPDATE {} SET {} WHERE {}",
            quote_ident(table),
            set_clause.join(", "),
            where_clause
        );
        let mut all_params: Vec<&(dyn postgres::types::ToSql + Sync)> =
            sets.iter().map(|(_, v)| *v).collect();
        all_params.extend(params.iter().copied());
        self.execute(&sql, &all_params)
    }

    /// 删除数据。`where_clause` + `params` 限定范围，返回受影响行数。
    pub fn delete(
        &mut self,
        table: &str,
        where_clause: &str,
        params: &[&(dyn postgres::types::ToSql + Sync)],
    ) -> PgResult<u64> {
        let sql = format!(
            "DELETE FROM {} WHERE {}",
            quote_ident(table),
            where_clause
        );
        self.execute(&sql, params)
    }

    // ===================== 原生 SQL =====================

    /// 执行任意 SQL（DDL / 无结果集语句），返回受影响行数。
    pub fn execute(
        &mut self,
        sql: &str,
        params: &[&(dyn postgres::types::ToSql + Sync)],
    ) -> PgResult<u64> {
        self.conn()?.execute(sql, params).map_err(PgError::Postgres)
    }

    /// 执行查询 SQL，返回行集合。
    pub fn query(
        &mut self,
        sql: &str,
        params: &[&(dyn postgres::types::ToSql + Sync)],
    ) -> PgResult<Vec<Row>> {
        self.conn()?.query(sql, params).map_err(PgError::Postgres)
    }
}

/// 事务包装器，方法签名与 [`PgClient`] 一致；未显式 commit 时 drop 自动回滚。
pub struct PgTransaction<'a> {
    tx: Transaction<'a>,
}

impl PgTransaction<'_> {
    pub fn create_table(&mut self, table: &str, columns: &[(&str, &str)]) -> PgResult<u64> {
        if columns.is_empty() {
            return Err(PgError::InvalidArgument(
                "create_table requires at least one column".into(),
            ));
        }
        let cols: Vec<String> = columns
            .iter()
            .map(|(name, ty)| format!("{} {}", quote_ident(name), ty))
            .collect();
        self.tx
            .execute(
                &format!("CREATE TABLE {} ({})", quote_ident(table), cols.join(", ")),
                &[],
            )
            .map_err(PgError::Postgres)
    }

    #[allow(clippy::type_complexity)]
    pub fn select(
        &mut self,
        table: &str,
        columns: Option<&[&str]>,
        where_clause: Option<&str>,
        params: &[&(dyn postgres::types::ToSql + Sync)],
        order_by: Option<&str>,
        limit: Option<i64>,
    ) -> PgResult<Vec<Row>> {
        let cols = columns
            .map(|c| c.iter().map(|n| quote_ident(n)).collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| "*".to_string());
        let mut sql = format!("SELECT {cols} FROM {}", quote_ident(table));
        if let Some(w) = where_clause {
            sql.push_str(" WHERE ");
            sql.push_str(w);
        }
        if let Some(o) = order_by {
            sql.push_str(" ORDER BY ");
            sql.push_str(o);
        }
        if let Some(l) = limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        self.tx.query(&sql, params).map_err(PgError::Postgres)
    }

    pub fn insert(
        &mut self,
        table: &str,
        values: &[(&str, &(dyn postgres::types::ToSql + Sync))],
    ) -> PgResult<u64> {
        let cols: Vec<String> = values.iter().map(|(c, _)| quote_ident(c)).collect();
        let placeholders: Vec<String> = (1..=values.len()).map(|i| format!("${i}")).collect();
        let params: Vec<&(dyn postgres::types::ToSql + Sync)> =
            values.iter().map(|(_, v)| *v).collect();
        self.tx
            .execute(
                &format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    quote_ident(table),
                    cols.join(", "),
                    placeholders.join(", ")
                ),
                &params,
            )
            .map_err(PgError::Postgres)
    }

    pub fn commit(self) -> PgResult<()> {
        self.tx.commit().map_err(PgError::Postgres)
    }

    pub fn rollback(self) -> PgResult<()> {
        self.tx.rollback().map_err(PgError::Postgres)
    }
}

/// 标识符加双引号并转义内嵌双引号，防注入。
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 将 where 子句中的 $1..$n 占位符平移为 $n+1..（配合 update 的 set 参数）。
fn shift_placeholders(clause: &str, offset: usize) -> PgResult<String> {
    if offset == 0 {
        return Ok(clause.to_string());
    }
    let mut out = String::with_capacity(clause.len());
    let mut chars = clause.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut num = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            if num.is_empty() {
                return Err(PgError::InvalidArgument(format!(
                    "dangling '$' in where clause: {clause}"
                )));
            }
            let idx: usize = num
                .parse()
                .map_err(|_| PgError::InvalidArgument(format!("bad placeholder: ${num}")))?;
            out.push_str(&format!("${}", idx + offset));
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_ident() {
        assert_eq!(quote_ident("users"), "\"users\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn test_shift_placeholders() {
        assert_eq!(
            shift_placeholders("age > $1 AND id < $2", 2).unwrap(),
            "age > $3 AND id < $4"
        );
        assert_eq!(shift_placeholders("id = $1", 0).unwrap(), "id = $1");
        assert!(shift_placeholders("id = $", 1).is_err());
    }

    #[test]
    fn test_dsn_display_masks_password() {
        let c = PgClient::new("localhost", 5432, "admin", "s3cret", "db");
        assert_eq!(c.dsn_display(), "postgres://admin:***@localhost:5432/db");
    }

    #[test]
    fn test_from_dsn() {
        let c = PgClient::from_dsn("postgres://u:p@db.example.com:5433/mydb").unwrap();
        assert_eq!(c.dsn_display(), "postgres://u:***@db.example.com:5433/mydb");
        assert!(PgClient::from_dsn("not a url").is_err());
    }

    #[test]
    fn test_not_connected() {
        let mut c = PgClient::new("localhost", 5432, "u", "p", "db");
        assert!(matches!(
            c.select("t", None, None, &[], None, None),
            Err(PgError::NotConnected)
        ));
    }
}
