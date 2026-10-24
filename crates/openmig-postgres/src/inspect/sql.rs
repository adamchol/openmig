//! Catalog queries. Each returns every object of one kind in the schemas passed as `$1`
//! (`oid[]`), so a whole database is read with a fixed number of round trips. They run with an
//! empty `search_path`, so names printed by `format_type`, `pg_get_expr` and friends are qualified
//! unless they live in `pg_catalog`.

/// JSON array of `[grantee, privilege, grantable]` for an ACL column, replacing NULL by
/// `acldefault(kind, owner)` when `kind` is given, without entries for the owner, the bootstrap
/// superuser and the current user.
fn acl(column: &str, kind: Option<char>, owner: &str) -> String {
    let source = match kind {
        Some(k) => format!("coalesce({column}, acldefault('{k}', {owner}))"),
        None => column.to_owned(),
    };
    format!(
        "(SELECT coalesce(json_agg(json_build_array(\
           CASE WHEN a.grantee = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(a.grantee)::text END, \
           a.privilege_type, a.is_grantable)), '[]') \
         FROM aclexplode({source}) a \
         WHERE a.grantee NOT IN ({owner}, 10, (SELECT r.oid FROM pg_roles r WHERE r.rolname = current_user)))"
    )
}

/// SQL text of a possibly qualified name, qualified unless it is in `pg_catalog`.
fn qualified(schema: &str, name: &str) -> String {
    format!(
        "CASE WHEN {schema} = 'pg_catalog' THEN quote_ident({name}) \
         ELSE quote_ident({schema}) || '.' || quote_ident({name}) END"
    )
}

fn not_extension_member(class: &str, oid: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM pg_depend xd WHERE xd.classid = '{class}'::regclass \
         AND xd.objid = {oid} AND xd.deptype = 'e')"
    )
}

/// `$1` is the history schema.
pub(super) fn schemas() -> String {
    format!(
        "SELECT n.oid, n.nspname::text AS name, pg_get_userbyid(n.nspowner)::text AS owner, \
           obj_description(n.oid, 'pg_namespace') AS comment, {acl} AS acl \
         FROM pg_namespace n \
         WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema' AND n.nspname <> $1 \
           AND {member}",
        acl = acl("n.nspacl", Some('n'), "n.nspowner"),
        member = not_extension_member("pg_namespace", "n.oid"),
    )
}

pub(super) fn relations() -> String {
    format!(
        "SELECT c.oid, c.relnamespace AS nsp, c.relname::text AS name, c.relkind::text AS kind, \
           c.relpersistence::text AS persistence, c.relrowsecurity AS rls, \
           c.relforcerowsecurity AS rls_forced, c.relreplident::text AS replident, \
           coalesce(c.reloptions, '{{}}') AS options, c.relispopulated AS populated, \
           pg_get_userbyid(c.relowner)::text AS owner, \
           obj_description(c.oid, 'pg_class') AS comment, {acl} AS acl, \
           CASE WHEN c.relkind IN ('v', 'm') THEN pg_get_viewdef(c.oid, true) END AS query, \
           CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END AS partkey, \
           c.relispartition AS is_partition, \
           CASE WHEN c.relispartition THEN pg_get_expr(c.relpartbound, c.oid) END AS partbound, \
           (SELECT json_agg(json_build_array(pn.nspname, pc.relname) ORDER BY i.inhseqno) \
            FROM pg_inherits i JOIN pg_class pc ON pc.oid = i.inhparent \
            JOIN pg_namespace pn ON pn.oid = pc.relnamespace WHERE i.inhrelid = c.oid) AS parents \
         FROM pg_class c \
         WHERE c.relnamespace = ANY($1) AND c.relkind IN ('r', 'p', 'v', 'm') AND {member}",
        acl = acl("c.relacl", Some('r'), "c.relowner"),
        member = not_extension_member("pg_class", "c.oid"),
    )
}

/// Attributes of tables, views, materialized views and composite types.
pub(super) fn columns() -> String {
    format!(
        "SELECT a.attrelid AS rel, a.attnum AS num, a.attname::text AS name, \
           format_type(a.atttypid, a.atttypmod) AS type, a.attnotnull AS not_null, \
           CASE WHEN a.atthasdef THEN pg_get_expr(ad.adbin, ad.adrelid, true) END AS default, \
           a.attidentity::text AS identity, a.attgenerated::text AS generated, \
           CASE WHEN a.attcollation <> t.typcollation THEN {collation} END AS collation, \
           CASE WHEN a.attstorage <> t.typstorage THEN a.attstorage::text END AS storage, \
           a.attcompression::text AS compression, \
           col_description(a.attrelid, a.attnum) AS comment, {acl} AS acl \
         FROM pg_attribute a \
         JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_type t ON t.oid = a.atttypid \
         LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
         LEFT JOIN pg_collation co ON co.oid = a.attcollation \
         LEFT JOIN pg_namespace con ON con.oid = co.collnamespace \
         WHERE c.relnamespace = ANY($1) AND c.relkind IN ('r', 'p', 'v', 'm', 'c') \
           AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attrelid, a.attnum",
        collation = qualified("con.nspname", "co.collname"),
        acl = acl("a.attacl", None, "c.relowner"),
    )
}

/// Indexes of tables and materialized views, except partition indexes attached to an index of
/// the partitioned parent.
pub(super) fn indexes(major: u32) -> String {
    let nulls_not_distinct = if major >= 15 {
        "i.indnullsnotdistinct"
    } else {
        "false"
    };
    format!(
        "SELECT i.indexrelid AS oid, i.indrelid AS rel, ic.relname::text AS name, \
           am.amname::text AS method, i.indisunique AS is_unique, \
           i.indisreplident AS replident, {nulls_not_distinct} AS nulls_not_distinct, \
           pg_get_expr(i.indpred, i.indrelid, true) AS predicate, \
           coalesce(ic.reloptions, '{{}}') AS options, \
           obj_description(i.indexrelid, 'pg_class') AS comment, \
           (SELECT json_agg(json_build_object(\
              'column', a.attname, \
              'expr', CASE WHEN i.indkey[k.n] = 0 THEN pg_get_indexdef(i.indexrelid, k.n + 1, true) END, \
              'key', k.n < i.indnkeyatts, \
              'opclass', CASE WHEN k.n < i.indnkeyatts AND NOT opc.opcdefault THEN {opclass} END, \
              'collation', CASE WHEN k.n < i.indnkeyatts AND i.indcollation[k.n] <> 0 \
                 AND i.indcollation[k.n] <> coalesce(a.attcollation, it.typcollation) THEN {collation} END, \
              'desc', k.n < i.indnkeyatts AND (i.indoption[k.n] & 1) <> 0, \
              'nulls_first', k.n < i.indnkeyatts AND (i.indoption[k.n] & 2) <> 0) ORDER BY k.n) \
            FROM generate_series(0, i.indnatts - 1) k(n) \
            LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[k.n] \
              AND i.indkey[k.n] <> 0 \
            LEFT JOIN pg_attribute ia ON ia.attrelid = i.indexrelid AND ia.attnum = k.n + 1 \
            LEFT JOIN pg_type it ON it.oid = ia.atttypid \
            LEFT JOIN pg_opclass opc ON opc.oid = i.indclass[k.n] \
            LEFT JOIN pg_namespace opn ON opn.oid = opc.opcnamespace \
            LEFT JOIN pg_collation co ON co.oid = i.indcollation[k.n] \
            LEFT JOIN pg_namespace con ON con.oid = co.collnamespace) AS keys \
         FROM pg_index i \
         JOIN pg_class ic ON ic.oid = i.indexrelid \
         JOIN pg_class t ON t.oid = i.indrelid \
         JOIN pg_am am ON am.oid = ic.relam \
         WHERE t.relnamespace = ANY($1) AND t.relkind IN ('r', 'p', 'm') \
           AND NOT EXISTS (SELECT 1 FROM pg_inherits inh WHERE inh.inhrelid = i.indexrelid)",
        opclass = qualified("opn.nspname", "opc.opcname"),
        collation = qualified("con.nspname", "co.collname"),
    )
}

/// Table and domain constraints that are declared locally (not inherited from a parent).
pub(super) fn constraints(major: u32) -> String {
    let enforced = if major >= 18 {
        "con.conenforced"
    } else {
        "true"
    };
    format!(
        "SELECT con.oid, con.conrelid AS rel, con.contypid AS typ, con.conname::text AS name, \
           con.contype::text AS type, con.condeferrable AS deferrable, \
           con.condeferred AS deferred, con.convalidated AS validated, \
           con.connoinherit AS no_inherit, {enforced} AS enforced, con.conindid AS index, \
           ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, o) \
                 JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n \
                 ORDER BY k.o) AS columns, \
           fn.nspname::text AS ref_schema, fc.relname::text AS ref_table, \
           ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, o) \
                 JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n \
                 ORDER BY k.o) AS ref_columns, \
           con.confupdtype::text AS on_update, con.confdeltype::text AS on_delete, \
           con.confmatchtype::text AS match, \
           ARRAY(SELECT CASE WHEN o.oprnamespace = 'pg_catalog'::regnamespace \
                   THEN o.oprname::text \
                   ELSE 'OPERATOR(' || quote_ident(opn.nspname) || '.' || o.oprname || ')' END \
                 FROM unnest(con.conexclop) WITH ORDINALITY x(op, o) \
                 JOIN pg_operator o ON o.oid = x.op \
                 JOIN pg_namespace opn ON opn.oid = o.oprnamespace ORDER BY x.o) AS operators, \
           CASE WHEN con.contype = 'c' THEN pg_get_expr(con.conbin, con.conrelid, true) END AS expr, \
           obj_description(con.oid, 'pg_constraint') AS comment \
         FROM pg_constraint con \
         LEFT JOIN pg_class fc ON fc.oid = con.confrelid \
         LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
         WHERE con.connamespace = ANY($1) AND con.contype IN ('p', 'u', 'f', 'c', 'x') \
           AND con.conparentid = 0 AND con.conislocal"
    )
}

/// User triggers, except internal ones (foreign keys) and clones on partitions.
pub(super) fn triggers() -> String {
    "SELECT t.oid, t.tgrelid AS rel, t.tgname::text AS name, t.tgtype::int4 AS type, \
       t.tgenabled::text AS enabled, \
       ARRAY(SELECT a.attname::text FROM unnest(t.tgattr::int2[]) WITH ORDINALITY k(n, o) \
             JOIN pg_attribute a ON a.attrelid = t.tgrelid AND a.attnum = k.n ORDER BY k.o) \
         AS update_of, \
       t.tgnargs::int4 AS nargs, t.tgargs AS args, t.tgconstraint <> 0 AS is_constraint, \
       t.tgdeferrable AS deferrable, t.tginitdeferred AS deferred, \
       frn.nspname::text AS from_schema, frc.relname::text AS from_table, \
       t.tgoldtable::text AS old_table, t.tgnewtable::text AS new_table, \
       pn.nspname::text AS func_schema, p.proname::text AS func_name, \
       CASE WHEN t.tgqual IS NOT NULL THEN pg_get_triggerdef(t.oid, true) END AS def, \
       obj_description(t.oid, 'pg_trigger') AS comment \
     FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_proc p ON p.oid = t.tgfoid \
     JOIN pg_namespace pn ON pn.oid = p.pronamespace \
     LEFT JOIN pg_class frc ON frc.oid = t.tgconstrrelid \
     LEFT JOIN pg_namespace frn ON frn.oid = frc.relnamespace \
     WHERE c.relnamespace = ANY($1) AND NOT t.tgisinternal AND t.tgparentid = 0"
        .to_owned()
}

pub(super) fn policies() -> String {
    "SELECT p.oid, p.polrelid AS rel, p.polname::text AS name, p.polcmd::text AS cmd, \
       p.polpermissive AS permissive, \
       ARRAY(SELECT CASE WHEN r = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(r)::text END \
             FROM unnest(p.polroles) r) AS roles, \
       pg_get_expr(p.polqual, p.polrelid, true) AS using, \
       pg_get_expr(p.polwithcheck, p.polrelid, true) AS check, \
       obj_description(p.oid, 'pg_policy') AS comment \
     FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
     WHERE c.relnamespace = ANY($1)"
        .to_owned()
}

/// Functions, window functions and procedures (aggregates are not modelled).
pub(super) fn functions() -> String {
    format!(
        "SELECT p.oid, p.pronamespace AS nsp, p.proname::text AS name, p.prokind::text AS kind, \
           l.lanname::text AS language, p.prosrc AS src, \
           CASE WHEN p.prosqlbody IS NOT NULL THEN pg_get_function_sqlbody(p.oid) END AS sqlbody, \
           p.provolatile::text AS volatility, p.proisstrict AS strict, \
           p.prosecdef AS security_definer, p.proleakproof AS leakproof, \
           p.proparallel::text AS parallel, p.procost::text AS cost, p.prorows::text AS rows, \
           p.proretset AS retset, format_type(p.prorettype, NULL) AS rettype, \
           coalesce(p.proconfig, '{{}}') AS config, \
           (SELECT json_agg(json_build_array(p.proargnames[k], \
                coalesce(p.proargmodes[k]::text, 'i'), \
                format_type(coalesce(p.proallargtypes[k], p.proargtypes[k - 1]), NULL), \
                pg_get_function_arg_default(p.oid, k)) ORDER BY k) \
            FROM generate_series(1, coalesce(array_length(p.proallargtypes, 1), p.pronargs)) k) \
             AS args, \
           pg_get_userbyid(p.proowner)::text AS owner, \
           obj_description(p.oid, 'pg_proc') AS comment, {acl} AS acl \
         FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang \
         WHERE p.pronamespace = ANY($1) AND p.prokind IN ('f', 'p', 'w') AND {member}",
        acl = acl("p.proacl", Some('f'), "p.proowner"),
        member = not_extension_member("pg_proc", "p.oid"),
    )
}

/// Enums, domains and composite types.
pub(super) fn types() -> String {
    format!(
        "SELECT t.oid, t.typnamespace AS nsp, t.typname::text AS name, t.typtype::text AS type, \
           t.typrelid AS rel, \
           ARRAY(SELECT e.enumlabel::text FROM pg_enum e WHERE e.enumtypid = t.oid \
                 ORDER BY e.enumsortorder) AS labels, \
           CASE WHEN t.typtype = 'd' THEN format_type(t.typbasetype, t.typtypmod) END AS base, \
           t.typnotnull AS not_null, pg_get_expr(t.typdefaultbin, 0, true) AS default, \
           CASE WHEN t.typtype = 'd' AND t.typcollation <> bt.typcollation THEN {collation} END \
             AS collation, \
           pg_get_userbyid(t.typowner)::text AS owner, \
           obj_description(t.oid, 'pg_type') AS comment, {acl} AS acl \
         FROM pg_type t \
         LEFT JOIN pg_type bt ON bt.oid = t.typbasetype \
         LEFT JOIN pg_class c ON c.oid = t.typrelid \
         LEFT JOIN pg_collation co ON co.oid = t.typcollation \
         LEFT JOIN pg_namespace con ON con.oid = co.collnamespace \
         WHERE t.typnamespace = ANY($1) \
           AND (t.typtype IN ('e', 'd') OR (t.typtype = 'c' AND c.relkind = 'c')) AND {member}",
        collation = qualified("con.nspname", "co.collname"),
        acl = acl("t.typacl", Some('T'), "t.typowner"),
        member = not_extension_member("pg_type", "t.oid"),
    )
}

/// Array types and row types of relations, for resolving dependencies on them.
pub(super) fn derived_types() -> String {
    "SELECT t.oid, t.typelem AS elem, t.typrelid AS rel FROM pg_type t \
     WHERE t.typnamespace = ANY($1) AND t.typtype NOT IN ('e', 'd') \
       AND (t.typrelid <> 0 OR (t.typelem <> 0 AND t.typsubscript = 'array_subscript_handler'::regproc))"
        .to_owned()
}

/// Sequences with the column that owns them (`a`: `OWNED BY`, `i`: identity).
pub(super) fn sequences() -> String {
    format!(
        "SELECT c.oid, c.relnamespace AS nsp, c.relname::text AS name, \
           format_type(s.seqtypid, NULL) AS type, s.seqstart AS start, \
           s.seqincrement AS increment, s.seqmin AS min, s.seqmax AS max, s.seqcache AS cache, \
           s.seqcycle AS cycle, d.deptype::text AS deptype, d.refobjid AS owner_rel, \
           d.refobjsubid::int2 AS owner_col, otn.nspname::text AS owner_schema, \
           ot.relname::text AS owner_table, oa.attname::text AS owner_column, \
           pg_get_userbyid(c.relowner)::text AS owner, \
           obj_description(c.oid, 'pg_class') AS comment, {acl} AS acl \
         FROM pg_sequence s \
         JOIN pg_class c ON c.oid = s.seqrelid \
         LEFT JOIN pg_depend d ON d.classid = 'pg_class'::regclass AND d.objid = c.oid \
           AND d.refclassid = 'pg_class'::regclass AND d.refobjsubid > 0 AND d.deptype IN ('a', 'i') \
         LEFT JOIN pg_class ot ON ot.oid = d.refobjid \
         LEFT JOIN pg_namespace otn ON otn.oid = ot.relnamespace \
         LEFT JOIN pg_attribute oa ON oa.attrelid = d.refobjid AND oa.attnum = d.refobjsubid \
         WHERE c.relnamespace = ANY($1) AND {member}",
        acl = acl("c.relacl", Some('s'), "c.relowner"),
        member = not_extension_member("pg_class", "c.oid"),
    )
}

pub(super) fn extensions() -> String {
    "SELECT e.oid, e.extname::text AS name, n.nspname::text AS schema, e.extversion AS version, \
       obj_description(e.oid, 'pg_extension') AS comment \
     FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace \
     WHERE e.extname <> 'plpgsql'"
        .to_owned()
}

pub(super) fn event_triggers() -> String {
    format!(
        "SELECT e.oid, e.evtname::text AS name, e.evtevent::text AS event, \
           e.evtenabled::text AS enabled, coalesce(e.evttags, '{{}}') AS tags, \
           pn.nspname::text AS func_schema, p.proname::text AS func_name, \
           pg_get_userbyid(e.evtowner)::text AS owner, \
           obj_description(e.oid, 'pg_event_trigger') AS comment \
         FROM pg_event_trigger e \
         JOIN pg_proc p ON p.oid = e.evtfoid \
         JOIN pg_namespace pn ON pn.oid = p.pronamespace \
         WHERE {member}",
        member = not_extension_member("pg_event_trigger", "e.oid"),
    )
}

/// Roles except built-in ones, the bootstrap superuser and the current user.
pub(super) fn roles() -> String {
    "SELECT r.oid, r.rolname::text AS name, r.rolsuper AS superuser, r.rolinherit AS inherit, \
       r.rolcreaterole AS createrole, r.rolcreatedb AS createdb, r.rolcanlogin AS login, \
       r.rolreplication AS replication, r.rolbypassrls AS bypassrls, \
       r.rolconnlimit AS connection_limit, r.rolvaliduntil::text AS valid_until, \
       shobj_description(r.oid, 'pg_authid') AS comment \
     FROM pg_roles r \
     WHERE r.rolname !~ '^pg_' AND r.oid <> 10 AND r.rolname <> current_user"
        .to_owned()
}

pub(super) fn memberships(major: u32) -> String {
    let options = if major >= 16 {
        "m.inherit_option AS inherit, m.set_option AS set"
    } else {
        "NULL::bool AS inherit, NULL::bool AS set"
    };
    format!(
        "SELECT m.member, g.rolname::text AS role, m.admin_option AS admin, {options} \
         FROM pg_auth_members m JOIN pg_roles g ON g.oid = m.roleid"
    )
}

/// Dependencies between non-system objects (and on schemas, as `public` has a system OID).
pub(super) fn depends() -> String {
    "SELECT d.classid::regclass::text AS class, d.objid, d.objsubid, \
       d.refclassid::regclass::text AS refclass, d.refobjid, d.refobjsubid \
     FROM pg_depend d \
     WHERE d.deptype IN ('n', 'a', 'i') AND d.objid >= 16384 \
       AND (d.refobjid >= 16384 OR d.refclassid = 'pg_namespace'::regclass)"
        .to_owned()
}

/// Objects that belong to an extension.
pub(super) fn extension_members() -> String {
    "SELECT d.classid::regclass::text AS class, d.objid, e.extname::text AS extension \
     FROM pg_depend d JOIN pg_extension e ON e.oid = d.refobjid \
     WHERE d.refclassid = 'pg_extension'::regclass AND d.deptype = 'e'"
        .to_owned()
}

/// Rewrite rules (view definitions) and column defaults, mapped to their relation.
pub(super) fn rules_and_defaults() -> String {
    "SELECT 'pg_rewrite' AS class, r.oid, r.ev_class AS rel, 0::int2 AS num \
     FROM pg_rewrite r JOIN pg_class c ON c.oid = r.ev_class WHERE c.relnamespace = ANY($1) \
     UNION ALL \
     SELECT 'pg_attrdef', ad.oid, ad.adrelid, ad.adnum \
     FROM pg_attrdef ad JOIN pg_class c ON c.oid = ad.adrelid WHERE c.relnamespace = ANY($1)"
        .to_owned()
}

/// Roles named by policies.
pub(super) fn policy_roles() -> String {
    "SELECT s.objid AS policy, r.rolname::text AS role \
     FROM pg_shdepend s JOIN pg_roles r ON r.oid = s.refobjid \
     WHERE s.dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
       AND s.classid = 'pg_policy'::regclass AND s.refclassid = 'pg_authid'::regclass \
       AND s.deptype = 'r'"
        .to_owned()
}
