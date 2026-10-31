-- expect: customerId=nullable line1=nullable
INSERT INTO addresses (customer_id, line1) VALUES (:customerId::int, :line1) LIMIT 0
