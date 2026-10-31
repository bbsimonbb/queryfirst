-- expect: email=required:where-eq-notnull limit=nullable offset=nullable
SELECT id FROM customers WHERE email = :email ORDER BY id LIMIT :limit OFFSET :offset
