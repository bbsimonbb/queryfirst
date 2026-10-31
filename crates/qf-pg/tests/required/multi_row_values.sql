-- expect: a=required:insert-notnull b=required:insert-notnull c=nullable d=required:insert-notnull e=required:insert-notnull f=nullable
INSERT INTO addresses (customer_id, line1, city) VALUES (:a, :b, :c), (:d, :e, :f)
