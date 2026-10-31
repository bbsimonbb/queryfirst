-- expect: email=notnull line1=nullable:outer-join total=notnull
SELECT c.email, a.line1, i.total
FROM customers c
  LEFT JOIN addresses a ON a.customer_id = c.id
  JOIN invoices i ON i.address_id = a.id
