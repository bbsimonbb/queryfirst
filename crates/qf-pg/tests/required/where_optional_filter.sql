-- expect: email=nullable
SELECT id FROM customers WHERE (:email::text IS NULL OR email = :email)
