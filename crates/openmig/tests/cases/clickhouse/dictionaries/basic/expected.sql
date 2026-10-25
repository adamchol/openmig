-- Replace dictionary "app"."country_names"
CREATE OR REPLACE DICTIONARY `app`.`country_names`
(
    `id` UInt64,
    `name` String DEFAULT '?'
)
PRIMARY KEY id
SOURCE(CLICKHOUSE(TABLE 'countries' DB 'app'))
LIFETIME(MIN 60 MAX 600)
LAYOUT(FLAT())
COMMENT 'Country names by id';
