-- expect: email=nullable name=nullable
SELECT id FROM customers WHERE email IS NOT DISTINCT FROM :email AND email IS DISTINCT FROM :name
