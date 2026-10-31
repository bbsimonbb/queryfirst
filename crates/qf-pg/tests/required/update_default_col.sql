-- expect: access=nullable createdAt=nullable email=required:where-eq-notnull
UPDATE customers SET access = :access, created_at = :createdAt WHERE email = :email
