role "svc_base" {
  comment = "Base role"
}

role "svc_api" {
  login            = true
  connection_limit = 10
  member_of        = [role.svc_base]
}

role "svc_admin" {
  createrole = true
  inherit    = false
  valid_until = "2030-01-01T00:00:00Z"
}
