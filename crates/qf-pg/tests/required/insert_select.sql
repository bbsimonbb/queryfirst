-- expect: customerId=nullable line1=nullable id=nullable
INSERT INTO addresses (customer_id, line1) SELECT :customerId, :line1 FROM customers WHERE id = :id
