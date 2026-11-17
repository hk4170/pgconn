//! 端到端演示：连接本地 PG，完整跑一遍 DDL + CRUD + 事务。
//!
//! 运行：cargo run --example demo
//! 可通过环境变量覆盖：PG_HOST / PG_PORT / PG_USER / PG_PASSWORD / PG_DBNAME

use pgconn::PgClient;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = env_or("PG_HOST", "localhost");
    let port: u16 = env_or("PG_PORT", "5432").parse()?;
    let user = env_or("PG_USER", "postgres");
    let password = env_or("PG_PASSWORD", "test123");
    let dbname = env_or("PG_DBNAME", "demo");

    // 1. new() 配置 + connect() 建立连接
    let mut client = PgClient::new(&host, port, &user, &password, &dbname);
    println!("连接目标: {}", client.dsn_display());
    client.connect()?;
    println!("✓ 连接成功");

    // 2. create_table
    client.drop_table("users", true)?;
    client.create_table("users", &[
        ("id", "SERIAL PRIMARY KEY"),
        ("name", "TEXT NOT NULL"),
        ("age", "INT"),
        ("email", "TEXT UNIQUE"),
    ])?;
    println!("✓ 建表 users");

    // 3. insert
    client.insert("users", &[("name", &"Alice"), ("age", &30), ("email", &"alice@x.com")])?;
    client.insert("users", &[("name", &"Bob"), ("age", &17), ("email", &"bob@x.com")])?;
    client.insert("users", &[("name", &"C\"arol"), ("age", &25), ("email", &"carol@x.com")])?;
    println!("✓ 插入 3 行");

    // 4. select（带 where / order / limit）
    let rows = client.select(
        "users",
        Some(&["id", "name", "age"]),
        Some("age > $1"),
        &[&18],
        Some("age DESC"),
        Some(10),
    )?;
    println!("✓ select (age>18, 倒序):");
    for row in &rows {
        let name: String = row.get("name");
        let age: i32 = row.get("age");
        println!("    - {name}, {age}");
    }
    assert_eq!(rows.len(), 2, "应查到 Alice 和 Carol 两行");

    // 5. update（set + where 参数自动编号平移）
    let n = client.update("users", &[("age", &26)], "name = $1", &[&"Alice"])?;
    assert_eq!(n, 1);
    println!("✓ update Alice.age -> 26");

    // 6. delete
    let n = client.delete("users", "age < $1", &[&18])?;
    assert_eq!(n, 1);
    println!("✓ delete 未成年用户 1 行");

    // 7. 事务：出错回滚
    {
        let mut tx = client.begin()?;
        tx.insert("users", &[("name", &"T rolled back")])?;
        tx.rollback()?;
    }
    {
        let mut tx = client.begin()?;
        tx.insert("users", &[("name", &"T committed")])?;
        tx.commit()?;
    }
    let total = client.query("SELECT count(*) AS c FROM users", &[])?;
    let c: i64 = total[0].get("c");
    assert_eq!(c, 3, "回滚 + 提交后应剩 3 行");
    println!("✓ 事务回滚/提交验证通过（最终 {c} 行）");

    // 8. 原生 SQL
    client.execute("COMMENT ON TABLE users IS 'demo table'", &[])?;
    println!("✓ execute 原生 SQL");

    // 9. drop_table
    client.drop_table("users", true)?;
    println!("✓ drop_table");

    println!("\n全部端到端验证通过 🎉");
    Ok(())
}
