-- expect: count=notnull n=notnull
SELECT count(*), count(*) FILTER (WHERE c.name IS NULL) AS n FROM customers c
