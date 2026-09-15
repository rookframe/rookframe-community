use sqlx::postgres::PgPoolOptions;
use std::{env, time::Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter("info")
        .init();
    let command = env::args().nth(1);
    if command.as_deref() == Some("healthcheck") {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect_timeout(
            &"127.0.0.1:8080".parse()?,
            Duration::from_secs(3),
        )?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(b"GET /api/v1/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        anyhow::ensure!(
            response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"),
            "unhealthy"
        );
        return Ok(());
    }
    let database =
        env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let db = PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '10s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&database)
        .await
        .map_err(|_| anyhow::anyhow!("database connection failed"))?;
    let migrator = sqlx::migrate!("./migrations");
    if command.as_deref() == Some("migrate") {
        migrator
            .run(&db)
            .await
            .map_err(|_| anyhow::anyhow!("migration failed"))?;
        return Ok(());
    }
    anyhow::ensure!(
        command.is_none(),
        "supported commands: migrate, healthcheck"
    );
    let unapplied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success = true")
            .fetch_one(&db)
            .await
            .map_err(|_| anyhow::anyhow!("run migrations before serving"))?;
    anyhow::ensure!(
        unapplied == migrator.iter().count() as i64,
        "run migrations before serving"
    );
    let address = env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!("community_server_ready");
    let turn = rookframe_community::TurnProvider::from_env()?;
    tokio::spawn(rookframe_community::maintain_requests(db.clone()));
    axum::serve(
        listener,
        rookframe_community::router_with_turn(db, turn)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
