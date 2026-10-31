-- expect: email=notnull line1=nullable:outer-join
SELECT * FROM (SELECT c.email, a.line1 FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id) s
