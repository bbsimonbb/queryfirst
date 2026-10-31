-- expect: email=notnull line1=nullable:outer-join total=nullable:outer-join
SELECT c.email, a.line1, i.total
FROM customers c
  LEFT JOIN (addresses a JOIN invoices i ON i.address_id = a.id) ON a.customer_id = c.id
