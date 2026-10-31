-- expect: a=nullable b=required:insert-notnull
INSERT INTO addresses (customer_id, line1, city) VALUES (1, :a, NULL), (2, :b, :a)
