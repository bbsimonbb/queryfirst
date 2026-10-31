-- expect: t=nullable:expression n=nullable:expression
SELECT (SELECT i.total FROM invoices i WHERE i.customer_id = c.id LIMIT 1) AS t,
       (SELECT count(*) FROM invoices i WHERE i.customer_id = c.id) AS n
FROM customers c
