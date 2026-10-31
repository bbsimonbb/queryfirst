-- expect: a=nullable b=nullable
SELECT id FROM customers WHERE lower(email) = :a AND email = lower(:b)
