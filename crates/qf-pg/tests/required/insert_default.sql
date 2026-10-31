-- expect: email=required:insert-notnull access=nullable createdAt=nullable
INSERT INTO customers (email, access, created_at) VALUES (:email, :access, :createdAt)
