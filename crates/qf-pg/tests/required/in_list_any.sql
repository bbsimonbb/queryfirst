-- expect: email=nullable emails=nullable
SELECT id FROM customers WHERE email IN (:email) AND email = ANY(:emails)
