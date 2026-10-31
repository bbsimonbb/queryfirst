-- expect: customerId=nullable
INSERT INTO addresses (customer_id, line1) VALUES (:customerId)
