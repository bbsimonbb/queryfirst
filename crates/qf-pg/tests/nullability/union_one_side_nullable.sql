-- expect: id=nullable:analyzer-gave-up email=notnull
SELECT id, email FROM customers
UNION
SELECT a.id, c.email FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id
