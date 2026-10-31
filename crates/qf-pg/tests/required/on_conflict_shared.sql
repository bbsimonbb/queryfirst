-- expect: id=nullable customerId=nullable line1=nullable
INSERT INTO addresses (id, customer_id, line1) VALUES (:id, :customerId, :line1)
ON CONFLICT (id) DO UPDATE SET line1 = :line1 WHERE addresses.customer_id = :customerId::int
