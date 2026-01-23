//! A module for path manipulation that works with string slices.
//!
//! This module provides a `Path` struct that is a thin wrapper around `&str`,
//! offering various methods for path inspection and manipulation.

use alloc::{borrow::ToOwned, vec::Vec};

use super::pathbuf::PathBuf;

/// Represents a path slice, akin to `&str`.
///
/// This struct provides a number of methods for inspecting a path,
/// including breaking the path into its components, determining if it's
/// absolute, and more.
#[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct Path {
    inner: str,
}

impl Path {
    /// Creates a new `Path` from a string slice.
    ///
    /// This is a cost-free conversion.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// let path = Path::new("/usr/bin/ls");
    /// ```
    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> &Self {
        unsafe { &*(s.as_ref() as *const str as *const Path) }
    }

    /// Returns the underlying string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// let path = Path::new("/etc/passwd");
    /// assert_eq!(path.as_str(), "/etc/passwd");
    /// ```
    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// Determines whether the path is absolute.
    ///
    /// An absolute path starts with a `/`.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// assert!(Path::new("/home/user").is_absolute());
    /// assert!(!Path::new("home/user").is_absolute());
    /// ```
    pub fn is_absolute(&self) -> bool {
        self.inner.starts_with('/')
    }

    /// Determines whether the path is relative.
    ///
    /// A relative path does not start with a `/`.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// assert!(Path::new("src/main.rs").is_relative());
    /// assert!(!Path::new("/src/main.rs").is_relative());
    /// ```
    pub fn is_relative(&self) -> bool {
        !self.is_absolute()
    }

    /// Produces an iterator over the components of the path.
    ///
    /// Components are the non-empty parts of the path separated by `/`.
    /// Repeated separators are ignored.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// let path = Path::new("/some//path/./file.txt");
    /// let components: Vec<_> = path.components().collect();
    /// assert_eq!(components, vec!["some", "path", "file.txt"]);
    /// ```
    pub fn components(&self) -> Components<'_> {
        Components {
            remaining: &self.inner,
        }
    }

    /// Joins two paths together.
    ///
    /// This will allocate a new `PathBuf` to hold the joined path. If the
    /// `other` path is absolute, it replaces the current one.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    /// use libkernel::fs::pathbuf::PathBuf;
    //
    /// let path1 = Path::new("/usr/local");
    /// let path2 = Path::new("bin/rustc");
    /// assert_eq!(path1.join(path2), PathBuf::from("/usr/local/bin/rustc"));
    ///
    /// let path3 = Path::new("/etc/init.d");
    /// assert_eq!(path1.join(path3), PathBuf::from("/etc/init.d"));
    /// ```
    pub fn join(&self, other: &Path) -> PathBuf {
        let mut ret: PathBuf = PathBuf::with_capacity(self.inner.len() + other.inner.len());

        ret.push(self);
        ret.push(other);

        ret
    }

    /// Strips a prefix from the path.
    ///
    /// If the path starts with `base`, returns a new `Path` slice with the
    /// prefix removed. Otherwise, returns `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// let path = Path::new("/usr/lib/x86_64-linux-gnu");
    /// let prefix = Path::new("/usr");
    ///
    /// assert_eq!(path.strip_prefix(prefix), Some(Path::new("lib/x86_64-linux-gnu")));
    /// assert_eq!(prefix.strip_prefix(path), None);
    /// ```
    pub fn strip_prefix(&self, base: &Path) -> Option<&Path> {
        if self.inner.starts_with(&base.inner) {
            // If the prefixes are the same, and they have the same length, the
            // whole string is the prefix.
            if base.inner.len() == self.inner.len() {
                return Some(Path::new(""));
            }
            if self.inner.as_bytes().get(base.inner.len()) == Some(&b'/') {
                let stripped = &self.inner[base.inner.len()..];
                // If the base ends with a slash, we don't want a leading slash on the result
                if base.inner.ends_with('/') {
                    return Some(Path::new(stripped));
                }
                return Some(Path::new(&stripped[1..]));
            }
        }
        None
    }

    /// Returns the parent directory of the path.
    ///
    /// Returns `None` if the path is a root directory or has no parent.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// let path = Path::new("/foo/bar");
    /// assert_eq!(path.parent(), Some(Path::new("/foo")));
    ///
    /// let path_root = Path::new("/");
    /// assert_eq!(path_root.parent(), None);
    /// ```
    pub fn parent(&self) -> Option<&Path> {
        let mut components = self.components().collect::<Vec<_>>();
        if components.len() <= 1 {
            return None;
        }
        components.pop();
        let parent_len = components.iter().map(|s| s.len()).sum::<usize>() + components.len() - 1;
        let end = if self.is_absolute() {
            parent_len + 1
        } else {
            parent_len
        };

        Some(Path::new(&self.inner[..end]))
    }

    /// Returns the final component of the path, if there is one.
    ///
    /// This is the file name for a file path, or the directory name for a
    /// directory path.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::fs::path::Path;
    ///
    /// assert_eq!(Path::new("/home/user/file.txt").file_name(), Some("file.txt"));
    /// assert_eq!(Path::new("/home/user/").file_name(), Some("user"));
    /// assert_eq!(Path::new("/").file_name(), None);
    /// ```
    pub fn file_name(&self) -> Option<&str> {
        self.components().last()
    }
}

impl AsRef<Path> for str {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl ToOwned for Path {
    type Owned = PathBuf;

    fn to_owned(&self) -> Self::Owned {
        PathBuf::from(self.as_str())
    }
}

/// An iterator over the components of a `Path`.
#[derive(Clone, Debug)]
pub struct Components<'a> {
    remaining: &'a str,
}

impl<'a> Iterator for Components<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        // Trim leading slashes
        self.remaining = self.remaining.trim_start_matches('/');

        if self.remaining.is_empty() {
            return None;
        }

        match self.remaining.find('/') {
            Some(index) => {
                let component = &self.remaining[..index];
                self.remaining = &self.remaining[index..];
                if component == "." {
                    self.next()
                } else {
                    Some(component)
                }
            }
            None => {
                let component = self.remaining;
                self.remaining = "";
                if component == "." {
                    self.next()
                } else {
                    Some(component)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Path;
    use alloc::vec::Vec;

    #[test]
    fn test_new_path() {
        let p = Path::new("/a/b/c");
        assert_eq!(p.as_str(), "/a/b/c");
    }

    #[test]
    fn test_is_absolute() {
        assert!(Path::new("/").is_absolute());
        assert!(Path::new("/a/b").is_absolute());
        assert!(!Path::new("a/b").is_absolute());
        assert!(!Path::new("").is_absolute());
    }

    #[test]
    fn test_is_relative() {
        assert!(Path::new("a/b").is_relative());
        assert!(Path::new("").is_relative());
        assert!(!Path::new("/a/b").is_relative());
    }

    #[test]
    fn test_components() {
        let p = Path::new("/a/b/c");
        let mut comps = p.components();
        assert_eq!(comps.next(), Some("a"));
        assert_eq!(comps.next(), Some("b"));
        assert_eq!(comps.next(), Some("c"));
        assert_eq!(comps.next(), None);

        let p2 = Path::new("a//b/./c/");
        assert_eq!(p2.components().collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_join() {
        assert_eq!(Path::new("/a/b").join(Path::new("c/d")), "/a/b/c/d".into());
        assert_eq!(Path::new("/a/b/").join(Path::new("c")), "/a/b/c".into());
        assert_eq!(Path::new("a").join(Path::new("b")), "a/b".into());
        assert_eq!(Path::new("/a").join(Path::new("/b")), "/b".into());
        assert_eq!(Path::new("").join(Path::new("a")), "a".into());
    }

    #[test]
    fn test_strip_prefix() {
        let p = Path::new("/a/b/c");
        assert_eq!(p.strip_prefix(Path::new("/a")), Some(Path::new("b/c")));
        assert_eq!(p.strip_prefix(Path::new("/a/b")), Some(Path::new("c")));
        assert_eq!(p.strip_prefix(Path::new("/a/b/c")), Some(Path::new("")));
        assert_eq!(p.strip_prefix(Path::new("/d")), None);
        assert_eq!(p.strip_prefix(Path::new("/a/b/c/d")), None);
        assert_eq!(Path::new("/a/bc").strip_prefix(Path::new("/a/b")), None);
    }

    #[test]
    fn test_parent() {
        assert_eq!(Path::new("/a/b/c").parent(), Some(Path::new("/a/b")));
        assert_eq!(Path::new("/a/b").parent(), Some(Path::new("/a")));
        assert_eq!(Path::new("/a").parent(), None);
        assert_eq!(Path::new("/").parent(), None);
        assert_eq!(Path::new("a/b").parent(), Some(Path::new("a")));
        assert_eq!(Path::new("a").parent(), None);
    }

    #[test]
    fn test_file_name() {
        assert_eq!(Path::new("/a/b/c.txt").file_name(), Some("c.txt"));
        assert_eq!(Path::new("/a/b/").file_name(), Some("b"));
        assert_eq!(Path::new("c.txt").file_name(), Some("c.txt"));
        assert_eq!(Path::new("/").file_name(), None);
        assert_eq!(Path::new(".").file_name(), None);
    }
}
