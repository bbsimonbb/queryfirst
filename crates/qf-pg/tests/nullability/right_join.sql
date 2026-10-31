-- expect: email=nullable:outer-join line1=nullable:outer-join id=notnull
SELECT c.email, a.line1, i.id
FROM customers c
  JOIN addresses a ON a.customer_id = c.id
  RIGHT JOIN invoices i ON i.address_id = a.id
