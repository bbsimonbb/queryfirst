-- expect: a=nullable:expression b=nullable:expression c=nullable:expression d=nullable:expression e=nullable:outer-join f=nullable:expression
SELECT name = 'x' AS a, c.id = 1 OR name = 'x' AS b, NOT (birthday > '2000-01-01') AS c,
       c.id = $1 AS d, a.line1 = 'x' AS e, c.id = ANY ('{1,2}'::int[]) AS f
FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id
