-- expect: email=nullable
SELECT id FROM customers WHERE (:email IS NULL OR email = :email)
