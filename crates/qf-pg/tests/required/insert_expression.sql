-- expect: customerId=nullable line1=nullable
INSERT INTO addresses (customer_id, line1) VALUES (:customerId + 0, coalesce(:line1, 'x'))
