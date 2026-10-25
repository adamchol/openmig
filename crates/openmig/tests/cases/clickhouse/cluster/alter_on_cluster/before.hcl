database "repl" {
  engine = Replicated("/clickhouse/databases/repl", "{shard}", "{replica}")
}

database "plain" {}

table "repl.events" {
  engine = ReplicatedMergeTree("/clickhouse/tables/{uuid}/{shard}", "{replica}")
  column "id" {
    type = UInt64
  }
  order_by = [column.id]
}

table "plain.events" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  order_by = [column.id]
}

role "local_reader" {
  storage = local_directory
}
