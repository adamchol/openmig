-- Revoke on function "app"."secret"() from PUBLIC
REVOKE EXECUTE ON FUNCTION "app"."secret"() FROM PUBLIC;
-- Grant on function "app"."secret"() to "app_reader"
GRANT EXECUTE ON FUNCTION "app"."secret"() TO "app_reader";
