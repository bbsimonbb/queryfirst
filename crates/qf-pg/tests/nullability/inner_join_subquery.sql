-- expect: email=notnull id=notnull total=notnull
SELECT c.email, s.id, s.total
FROM customers c JOIN (SELECT id, customer_id, total FROM invoices) s ON s.customer_id = c.id
