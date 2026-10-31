//! A fresh temp schema loaded with `tests/fixtures/schema.sql`, for db-tests. Same approach as
//! `describe_db.rs`: the schema is selected through `search_path` and dropped afterwards.

use qf_pg::{Catalog, ConnSource, Pool, connect_pool, resolve_connection};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const FIXTURE: &str = include_str!("../fixtures/schema.sql");

pub struct Db {
    pub pool: Pool,
    pub catalog: Arc<Catalog>,
    pub admin: Arc<tokio_postgres::Client>,
    pub schema: String,
}

fn database_url() -> String {
    std::env::var("QF_TEST_DATABASE_URL")
        .expect("QF_TEST_DATABASE_URL must be set when running with --features db-tests")
}

fn unique_schema() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!(
        "qf_test_{}_{}_{}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn with_search_path(url: &str, schema: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}options=-c%20search_path%3D{schema}")
}

/// Create a temp schema with the fixture, run `body`, then drop the schema even if `body` panics.
pub async fn with_db<F, Fut>(body: F)
where
    F: FnOnce(Db) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let url = database_url();
    let schema = unique_schema();
    let (admin, conn) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_postgres::connect(&url, tokio_postgres::NoTls),
    )
    .await
    .expect("timed out connecting to QF_TEST_DATABASE_URL")
    .expect("cannot connect to QF_TEST_DATABASE_URL");
    tokio::spawn(conn);
    admin
        .batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema};\n{FIXTURE}"
        ))
        .await
        .expect("load fixture schema");
    let admin = Arc::new(admin);

    let resolved =
        resolve_connection(&[ConnSource::Explicit(with_search_path(&url, &schema))]).unwrap();
    let outcome = async {
        let pool = connect_pool(&resolved, 2, Duration::from_secs(10))
            .await
            .expect("connect_pool");
        let catalog = Arc::new(Catalog::load(&pool).await.expect("Catalog::load"));
        let db = Db {
            pool,
            catalog,
            admin: admin.clone(),
            schema: schema.clone(),
        };
        tokio::spawn(body(db)).await
    }
    .await;

    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .expect("drop temp schema");
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}
