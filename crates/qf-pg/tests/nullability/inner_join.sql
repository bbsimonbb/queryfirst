-- expect: email=notnull total=notnull line1=notnull
SELECT c.email, i.total, a.line1
FROM customers c
  JOIN invoices i ON i.customer_id = c.id
  CROSS JOIN addresses a
