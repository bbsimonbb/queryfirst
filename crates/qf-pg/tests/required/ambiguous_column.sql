-- expect: id=nullable
SELECT c.id FROM customers c JOIN addresses a ON a.customer_id = c.id WHERE id = :id
