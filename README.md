# pgconn

轻量级 Rust PostgreSQL 客户端库：`new()` 配置连接参数，方法名遵循 SQL 语法。

## 特性

- **同步 API**，基于 `postgres` crate 封装，无 async 运行时负担
- **SQL 命名方法**：`create_table` / `drop_table` / `select` / `insert` / `update` / `delete` / `execute` / `query`
- **防注入**：值参数全部走参数化查询；表名/列名自动加双引号并转义
- **事务支持**：`begin()` 返回事务对象，未显式 `commit()` 自动回滚
- **两种建连方式**：`new()` 或 `from_dsn()`

## 快速开始

```rust
use pgconn::PgClient;

let mut client = PgClient::new("localhost", 5432, "postgres", "secret", "mydb");
client.connect()?;   // 此时才真正建立连接

// DDL
client.create_table("users", &[
    ("id", "SERIAL PRIMARY KEY"),
    ("name", "TEXT NOT NULL"),
    ("age", "INT"),
])?;

// 插入（值参数化）
client.insert("users", &[("name", &"Alice"), ("age", &30)])?;

// 查询：columns / where / order_by / limit 均可选
let rows = client.select(
    "users",
    Some(&["id", "name"]),
    Some("age > $1"),   // 占位符对应下方 params
    &[&18],
    Some("id DESC"),
    Some(10),
)?;
for row in &rows {
    let name: String = row.get("name");
    println!("{name}");
}

// 更新（set 参数在前，where 占位符自动平移编号）
client.update("users", &[("age", &26)], "name = $1", &[&"Alice"])?;

// 删除
client.delete("users", "age < $1", &[&18])?;

// 事务
let mut tx = client.begin()?;
tx.insert("users", &[("name", &"Bob")])?;
tx.commit()?;   // 若改为 tx.rollback()? 则全部回滚

// 任意 SQL
client.execute("SELECT 1", &[])?;
client.drop_table("users", true)?;
```

DSN 方式：

```rust
let mut client = PgClient::from_dsn("postgres://user:pass@host:5432/db")?;
client.connect()?;
```

## API 一览

| 方法 | 说明 |
|---|---|
| `new(host, port, user, password, dbname)` | 创建连接配置（不连接） |
| `from_dsn(dsn)` | 从 `postgres://` 连接串创建 |
| `connect()` | 建立连接 |
| `create_table(table, &[(col, def)])` | 建表，列定义原样写入 |
| `drop_table(table, if_exists)` | 删表 |
| `select(table, cols?, where?, params, order_by?, limit?)` | 查询，返回 `Vec<Row>` |
| `insert(table, &[(col, value)])` | 插入，返回受影响行数 |
| `update(table, sets, where, params)` | 更新，返回受影响行数 |
| `delete(table, where, params)` | 删除，返回受影响行数 |
| `execute(sql, params)` / `query(sql, params)` | 原生 SQL |
| `begin()` | 开启事务（含 `create_table/select/insert`），`commit()` / `rollback()` |
| `dsn_display()` | 脱敏连接串（密码显示为 `***`） |

## 运行示例

```bash
# 默认连接 localhost:5432/demo，可通过环境变量覆盖
PG_HOST=localhost PG_PORT=5432 PG_USER=postgres PG_PASSWORD=test123 PG_DBNAME=demo \
  cargo run --example demo

cargo test    # 单元测试
```

## 依赖

- Rust 2021 edition
- `postgres` 0.19、`thiserror` 2、`url` 2
