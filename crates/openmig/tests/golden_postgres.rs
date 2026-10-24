#[path = "golden/harness.rs"]
mod harness;

fn main() {
    harness::main::<openmig_postgres::PostgresEngine>(harness::Kind::Postgres);
}
