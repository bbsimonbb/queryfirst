-- expect: customerId=required:insert-notnull line1=required:insert-notnull city=nullable
INSERT INTO addresses (customer_id, line1, city) VALUES (:customerId, :line1, :city)
