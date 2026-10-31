-- expect: id=nullable
SELECT id FROM customers c WHERE NOT EXISTS (SELECT 1 FROM addresses a WHERE a.customer_id = :id)
