-- expect: a=notnull b=notnull c=notnull d=nullable:expression
SELECT name IS NULL AS a, name IS DISTINCT FROM 'x' AS b, (name = 'x') IS TRUE AS c,
       nullif(email, 'x') AS d
FROM customers
