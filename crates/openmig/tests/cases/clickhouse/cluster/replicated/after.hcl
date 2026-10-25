variable "replicated" {
  type    = bool
  default = true
}

database "shop" {
  engine = var.replicated ? Replicated("/clickhouse/databases/shop", "{shard}", "{replica}") : Atomic
}

database "plain" {}

table "shop.events" {
  engine = ReplicatedMergeTree("/clickhouse/tables/{uuid}/{shard}", "{replica}")
  column "call_id" {
    type = String
  }
  column "timestamp" {
    type = DateTime64(9)
  }
  order_by = [column.timestamp]
}

table "plain.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime64(9)
  }
  order_by = [column.timestamp]
}
