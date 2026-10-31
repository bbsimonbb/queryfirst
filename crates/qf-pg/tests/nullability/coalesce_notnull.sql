-- expect: name=notnull line1=notnull
SELECT coalesce(c.name, '') AS name, coalesce(a.line1, c.email) AS line1
FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id
