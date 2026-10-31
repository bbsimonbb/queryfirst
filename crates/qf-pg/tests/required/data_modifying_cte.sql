-- expect: customerId=nullable line1=nullable
WITH ins AS (INSERT INTO addresses (customer_id, line1) VALUES (:customerId, :line1) RETURNING id) SELECT id FROM ins
