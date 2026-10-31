-- expect: tag=nullable
INSERT INTO customers (email, tags[1]) VALUES ('a', :tag)
