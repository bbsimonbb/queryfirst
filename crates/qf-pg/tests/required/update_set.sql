-- expect: email=required:insert-notnull name=nullable id=required:where-eq-notnull
UPDATE customers SET email = :email, name = :name WHERE id = :id
