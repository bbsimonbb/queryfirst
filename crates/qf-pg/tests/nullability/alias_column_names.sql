-- expect: email=nullable:expression b=notnull
SELECT s.email, x.b
FROM (SELECT name, id FROM customers) s(email, n)
  JOIN customers x(a, b) ON x.a = s.n
