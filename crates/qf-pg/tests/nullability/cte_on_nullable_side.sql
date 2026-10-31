-- expect: email=notnull total=nullable:outer-join
WITH inv AS (SELECT customer_id, total FROM invoices)
SELECT c.email, inv.total FROM customers c LEFT JOIN inv ON inv.customer_id = c.id
