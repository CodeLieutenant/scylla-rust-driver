use crate::frame::response::result::ColumnSpec;

use super::row::{mk_deser_err, BuiltinDeserializationErrorKind, ColumnIterator, DeserializeRow};
use super::{DeserializationError, FrameSlice, TypeCheckError};
use std::marker::PhantomData;

/// Iterates over the whole result, returning raw rows.
#[derive(Debug)]
pub struct RawRowIterator<'frame, 'metadata> {
    specs: &'metadata [ColumnSpec<'metadata>],
    remaining: usize,
    slice: FrameSlice<'frame>,
}

impl<'frame, 'metadata> RawRowIterator<'frame, 'metadata> {
    /// Creates a new iterator over raw rows from a serialized response.
    ///
    /// - `remaining` - number of the remaining rows in the serialized response,
    /// - `specs` - information about columns of the serialized response,
    /// - `slice` - a [FrameSlice] that points to the serialized rows data.
    #[inline]
    pub fn new(
        remaining: usize,
        specs: &'metadata [ColumnSpec<'metadata>],
        slice: FrameSlice<'frame>,
    ) -> Self {
        Self {
            specs,
            remaining,
            slice,
        }
    }

    /// Returns information about the columns of rows that are iterated over.
    #[inline]
    pub fn specs(&self) -> &'metadata [ColumnSpec<'metadata>] {
        self.specs
    }

    /// Returns the remaining number of rows that this iterator is supposed
    /// to return.
    #[inline]
    pub fn rows_remaining(&self) -> usize {
        self.remaining
    }
}

impl<'frame, 'metadata> Iterator for RawRowIterator<'frame, 'metadata> {
    type Item = Result<ColumnIterator<'frame, 'metadata>, DeserializationError>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.remaining = self.remaining.checked_sub(1)?;

        let iter = ColumnIterator::new(self.specs, self.slice);

        // Skip the row here, manually
        for (column_index, spec) in self.specs.iter().enumerate() {
            if let Err(err) = self.slice.read_cql_bytes() {
                return Some(Err(mk_deser_err::<Self>(
                    BuiltinDeserializationErrorKind::RawColumnDeserializationFailed {
                        column_index,
                        column_name: spec.name().to_owned(),
                        err: DeserializationError::new(err),
                    },
                )));
            }
        }

        Some(Ok(iter))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        // The iterator will always return exactly `self.remaining`
        // elements: Oks until an error is encountered and then Errs
        // containing that same first encountered error.
        (self.remaining, Some(self.remaining))
    }
}

/// A typed version of [RawRowIterator] which deserializes the rows before
/// returning them.
#[derive(Debug)]
pub struct TypedRowIterator<'frame, 'metadata, R> {
    inner: RawRowIterator<'frame, 'metadata>,
    _phantom: PhantomData<R>,
}

impl<'frame, 'metadata, R> TypedRowIterator<'frame, 'metadata, R>
where
    R: DeserializeRow<'frame, 'metadata>,
{
    /// Creates a new [TypedRowIterator] from given [RawRowIterator].
    ///
    /// Calls `R::type_check` and fails if the type check fails.
    #[inline]
    pub fn new(raw: RawRowIterator<'frame, 'metadata>) -> Result<Self, TypeCheckError> {
        R::type_check(raw.specs())?;
        Ok(Self {
            inner: raw,
            _phantom: PhantomData,
        })
    }

    /// Returns information about the columns of rows that are iterated over.
    #[inline]
    pub fn specs(&self) -> &'metadata [ColumnSpec<'metadata>] {
        self.inner.specs()
    }

    /// Returns the remaining number of rows that this iterator is supposed
    /// to return.
    #[inline]
    pub fn rows_remaining(&self) -> usize {
        self.inner.rows_remaining()
    }
}

impl<'frame, 'metadata, R> Iterator for TypedRowIterator<'frame, 'metadata, R>
where
    R: DeserializeRow<'frame, 'metadata>,
{
    type Item = Result<R, DeserializationError>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|raw| raw.and_then(|raw| R::deserialize(raw)))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use crate::frame::response::result::ColumnType;

    use super::super::tests::{serialize_cells, spec, CELL1, CELL2};
    use super::{FrameSlice, RawRowIterator, TypedRowIterator};

    #[test]
    fn test_row_iterator_basic_parse() {
        let raw_data = serialize_cells([Some(CELL1), Some(CELL2), Some(CELL2), Some(CELL1)]);
        let specs = [spec("b1", ColumnType::Blob), spec("b2", ColumnType::Blob)];
        let mut iter = RawRowIterator::new(2, &specs, FrameSlice::new(&raw_data));

        let mut row1 = iter.next().unwrap().unwrap();
        let c11 = row1.next().unwrap().unwrap();
        assert_eq!(c11.slice.unwrap().as_slice(), CELL1);
        let c12 = row1.next().unwrap().unwrap();
        assert_eq!(c12.slice.unwrap().as_slice(), CELL2);
        assert!(row1.next().is_none());

        let mut row2 = iter.next().unwrap().unwrap();
        let c21 = row2.next().unwrap().unwrap();
        assert_eq!(c21.slice.unwrap().as_slice(), CELL2);
        let c22 = row2.next().unwrap().unwrap();
        assert_eq!(c22.slice.unwrap().as_slice(), CELL1);
        assert!(row2.next().is_none());

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_row_iterator_too_few_rows() {
        let raw_data = serialize_cells([Some(CELL1), Some(CELL2)]);
        let specs = [spec("b1", ColumnType::Blob), spec("b2", ColumnType::Blob)];
        let mut iter = RawRowIterator::new(2, &specs, FrameSlice::new(&raw_data));

        iter.next().unwrap().unwrap();
        assert!(iter.next().unwrap().is_err());
    }

    #[test]
    fn test_typed_row_iterator_basic_parse() {
        let raw_data = serialize_cells([Some(CELL1), Some(CELL2), Some(CELL2), Some(CELL1)]);
        let specs = [spec("b1", ColumnType::Blob), spec("b2", ColumnType::Blob)];
        let iter = RawRowIterator::new(2, &specs, FrameSlice::new(&raw_data));
        let mut iter = TypedRowIterator::<'_, '_, (&[u8], Vec<u8>)>::new(iter).unwrap();

        let (c11, c12) = iter.next().unwrap().unwrap();
        assert_eq!(c11, CELL1);
        assert_eq!(c12, CELL2);

        let (c21, c22) = iter.next().unwrap().unwrap();
        assert_eq!(c21, CELL2);
        assert_eq!(c22, CELL1);

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_typed_row_iterator_wrong_type() {
        let raw_data = Bytes::new();
        let specs = [spec("b1", ColumnType::Blob), spec("b2", ColumnType::Blob)];
        let iter = RawRowIterator::new(0, &specs, FrameSlice::new(&raw_data));
        assert!(TypedRowIterator::<'_, '_, (i32, i64)>::new(iter).is_err());
    }
}
