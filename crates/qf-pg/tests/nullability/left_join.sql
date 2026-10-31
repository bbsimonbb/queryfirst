-- expect: email=notnull line1=nullable:outer-join city=nullable:outer-join
SELECT c.email, a.line1, a.city
FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id
