-- expect: email=required:insert-notnull
UPDATE customers SET email = :email WHERE email = :email
