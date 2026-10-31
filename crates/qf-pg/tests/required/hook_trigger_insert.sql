-- expect: title=nullable slug=nullable
INSERT INTO posts (title, slug) VALUES (:title, :slug)
