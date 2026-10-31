-- expect: a=notnull b=notnull c=notnull d=notnull e=notnull f=notnull g=notnull h=notnull
SELECT id = 1 AS a, email <> 'x' AS b, id > 0 AND access = 'admin' AS c,
       NOT (id < 5) OR created_at >= '2020-01-01' AS d, -id + 1 * 2 AS e,
       email LIKE '%@%' AS f, id IN (1, 2, 3) AS g, id BETWEEN 1 AND 3 AS h
FROM customers
