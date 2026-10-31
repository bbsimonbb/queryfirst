-- expect: email=nullable:outer-join line1=notnull
SELECT email, line1
FROM (SELECT c.email, a.line1 FROM customers c RIGHT JOIN addresses a ON a.customer_id = c.id) AS s
