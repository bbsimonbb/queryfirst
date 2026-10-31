-- expect: email=notnull id=nullable:outer-join total=nullable:outer-join
SELECT c.email, s.id, s.total
FROM customers c LEFT JOIN (SELECT id, customer_id, total FROM invoices) s ON s.customer_id = c.id
