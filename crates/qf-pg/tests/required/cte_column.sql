-- expect: id=required:where-eq-notnull
WITH x AS (SELECT id FROM customers) SELECT id FROM x WHERE x.id = :id
