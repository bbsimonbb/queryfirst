-- expect: id=nullable
SELECT c.id FROM customers c FULL JOIN addresses a ON a.customer_id = c.id AND c.id = :id
