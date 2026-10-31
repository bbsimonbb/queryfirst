-- expect: email=notnull total=nullable:outer-join
SELECT c.email, l.total
FROM customers c
  LEFT JOIN LATERAL (SELECT i.total FROM invoices i WHERE i.customer_id = c.id LIMIT 1) l ON true
