-- expect: flag=nullable email=nullable
SELECT id FROM customers WHERE email = CASE WHEN :flag THEN 'a' ELSE :email END
