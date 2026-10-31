-- expect: access=notnull count=notnull
SELECT access, count(*) FROM customers GROUP BY access
