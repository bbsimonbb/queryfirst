-- expect: id=nullable customerId=required:insert-notnull line1=required:insert-notnull newLine1=nullable
INSERT INTO addresses (id, customer_id, line1) VALUES (:id, :customerId, :line1)
ON CONFLICT (id) DO UPDATE SET line1 = :newLine1
