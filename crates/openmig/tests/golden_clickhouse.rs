#[path = "golden/harness.rs"]
mod harness;

fn main() {
    harness::main::<openmig_clickhouse::ClickHouseEngine>(harness::Kind::ClickHouse);
}
