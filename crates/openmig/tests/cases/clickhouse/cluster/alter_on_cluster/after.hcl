database "repl" {
  engine  = Replicated("/clickhouse/databases/repl", "{shard}", "{replica}")
  comment = "replicated"
}

database "plain" {}

table "repl.events" {
  engine = ReplicatedMergeTree("/clickhouse/tables/{uuid}/{shard}", "{replica}")
  column "id" {
    type = UInt64
  }
  column "kind" {
    type = LowCardinality(String)
  }
  order_by = [column.id]
}

table "plain.events" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  column "kind" {
    type = String
  }
  order_by = [column.id]
}

role "local_reader" {
  storage = local_directory
}

role "replicated_reader" {
  storage = replicated
}

function "double" {
  args = ["x"]
  body = "x * 2"
}

grant {
  privileges = [SELECT]
  on         = [database.repl, database.plain]
  to         = [role.local_reader, role.replicated_reader]
}
