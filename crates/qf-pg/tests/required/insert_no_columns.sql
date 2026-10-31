-- expect: customerId=required:insert-notnull line1=required:insert-notnull city=nullable
INSERT INTO addresses VALUES (DEFAULT, :customerId, :line1, :city)
