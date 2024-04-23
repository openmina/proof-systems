use ark_ff::PrimeField;
use o1_utils::FieldHelpers;
use strum::IntoEnumIterator;

use crate::{
    columns::{Column, ColumnIndexer},
    logup::LookupTableID,
    serialization::{
        column::{SerializationColumn, SER_N_COLUMNS},
        interpreter::InterpreterEnv,
        lookups::{Lookup, LookupTable},
    },
    witness::Witness,
};
use kimchi::circuits::domains::EvaluationDomains;
use std::{collections::BTreeMap, iter};

/// Environment for the serializer interpreter
pub struct WitnessBuilderEnv<F: PrimeField, Ff: PrimeField> {
    /// Single-row witness columns, in raw form. For accessing [`Witness`], see the
    /// `get_witness` method.
    pub witness: Witness<SER_N_COLUMNS, F>,

    /// Keep track of the lookup multiplicities.
    pub lookup_multiplicities: BTreeMap<LookupTable<Ff>, Vec<F>>,

    /// Keep track of the lookups for each row.
    pub lookups: BTreeMap<LookupTable<Ff>, Vec<Lookup<F, Ff>>>,
}

impl<F: PrimeField, Ff: PrimeField> InterpreterEnv<F, Ff> for WitnessBuilderEnv<F, Ff> {
    type Position = Column;

    // Requiring an F element as we would need to compute values up to 180 bits
    // in the 15 bits decomposition.
    type Variable = F;

    fn add_constraint(&mut self, cst: Self::Variable) {
        assert_eq!(cst, F::zero());
    }

    fn constant(value: F) -> Self::Variable {
        value
    }

    fn get_column(pos: SerializationColumn) -> Self::Position {
        pos.to_column()
    }

    fn read_column(&self, ix: Column) -> Self::Variable {
        let Column::X(i) = ix else { todo!() };
        self.witness.cols[i]
    }

    fn lookup(&mut self, table_id: LookupTable<Ff>, value: &Self::Variable) {
        let value_ix = table_id.ix_by_value(*value);
        self.lookup_multiplicities.get_mut(&table_id).unwrap()[value_ix] += F::one();
        self.lookups.get_mut(&table_id).unwrap().push(Lookup {
            table_id,
            numerator: F::one(),
            value: vec![*value],
        })
    }

    fn copy(&mut self, x: &Self::Variable, position: Self::Position) -> Self::Variable {
        self.write_column(position, *x);
        *x
    }

    /// Returns the bits between [highest_bit, lowest_bit] of the variable `x`,
    /// and copy the result in the column `position`.
    /// The value `x` is expected to be encoded in big-endian
    fn bitmask_be(
        &mut self,
        x: &Self::Variable,
        highest_bit: u32,
        lowest_bit: u32,
        position: Self::Position,
    ) -> Self::Variable {
        // FIXME: we can assume bitmask_be will be called only on value with
        // maximum 128 bits. We use bitmask_be only for the limbs
        let x_bytes_u8 = &x.to_bytes()[0..16];
        let x_u128 = u128::from_le_bytes(x_bytes_u8.try_into().unwrap());
        let res = (x_u128 >> lowest_bit) & ((1 << (highest_bit - lowest_bit)) - 1);
        let res_fp: F = res.into();
        self.write_column(position, res_fp);
        res_fp
    }
}

impl<F: PrimeField, Ff: PrimeField> WitnessBuilderEnv<F, Ff> {
    pub fn write_column(&mut self, position: Column, value: F) {
        match position {
            Column::X(i) => self.witness.cols[i] = value,
            Column::LookupPartialSum(_) => {
                panic!(
                    "This is a lookup related column. The environment is
                supposed to write only in witness columns"
                );
            }
            Column::LookupMultiplicity(_) => {
                panic!(
                    "This is a lookup related column. The environment is
                supposed to write only in witness columns"
                );
            }
            Column::LookupAggregation => {
                panic!(
                    "This is a lookup related column. The environment is
                supposed to write only in witness columns"
                );
            }
            Column::LookupFixedTable(_) => {
                panic!(
                    "This is a lookup related column. The environment is
                supposed to write only in witness columns"
                );
            }
        }
    }

    pub fn reset(&mut self) {
        for table in self.lookups.values_mut() {
            *table = Vec::new();
        }
        // TODO do we need to reset multiplicities?
    }

    /// Getting multiplicities for range check tables less or equal than 15 bits.
    pub fn get_lookup_multiplicities(
        &self,
        domain: EvaluationDomains<F>,
        table_id: LookupTable<Ff>,
    ) -> Vec<F> {
        let mut m = Vec::with_capacity(domain.d1.size as usize);
        m.extend(self.lookup_multiplicities[&table_id].to_vec());
        if table_id.length() < (domain.d1.size as usize) {
            let n_repeated_dummy_value: usize = (domain.d1.size as usize) - table_id.length() - 1;
            let repeated_dummy_value: Vec<F> = iter::repeat(-F::one())
                .take(n_repeated_dummy_value)
                .collect();
            m.extend(repeated_dummy_value);
            m.push(F::from(n_repeated_dummy_value as u64));
        }
        assert_eq!(m.len(), domain.d1.size as usize);
        m
    }
}

impl<F: PrimeField, Ff: PrimeField> WitnessBuilderEnv<F, Ff> {
    pub fn create() -> Self {
        let mut lookups = BTreeMap::new();
        let mut lookup_multiplicities = BTreeMap::new();
        for table_id in LookupTable::<Ff>::iter() {
            lookups.insert(table_id, Vec::new());
            lookup_multiplicities.insert(table_id, vec![F::zero(); table_id.length()]);
        }

        Self {
            witness: Witness {
                cols: Box::new([F::zero(); SER_N_COLUMNS]),
            },

            lookup_multiplicities,
            lookups,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        serialization::{
            column::SerializationColumn,
            interpreter::{deserialize_field_element, InterpreterEnv},
            witness::WitnessBuilderEnv,
            N_INTERMEDIATE_LIMBS,
        },
        Ff1, LIMB_BITSIZE, N_LIMBS,
    };
    use ark_ff::{BigInteger, FpParameters as _, One, PrimeField, UniformRand, Zero};
    use mina_curves::pasta::Fp;
    use num_bigint::BigUint;
    use o1_utils::{tests::make_test_rng, FieldHelpers};
    use rand::Rng;
    use std::str::FromStr;

    fn test_decomposition_generic(x: Fp) {
        let bits = x.to_bits();
        let limb0: u128 = {
            let limb0_le_bits: &[bool] = &bits.clone().into_iter().take(88).collect::<Vec<bool>>();
            let limb0 = Fp::from_bits(limb0_le_bits).unwrap();
            limb0.to_biguint().try_into().unwrap()
        };
        let limb1: u128 = {
            let limb0_le_bits: &[bool] = &bits
                .clone()
                .into_iter()
                .skip(88)
                .take(88)
                .collect::<Vec<bool>>();
            let limb0 = Fp::from_bits(limb0_le_bits).unwrap();
            limb0.to_biguint().try_into().unwrap()
        };
        let limb2: u128 = {
            let limb0_le_bits: &[bool] = &bits
                .clone()
                .into_iter()
                .skip(2 * 88)
                .take(79)
                .collect::<Vec<bool>>();
            let limb0 = Fp::from_bits(limb0_le_bits).unwrap();
            limb0.to_biguint().try_into().unwrap()
        };
        let mut dummy_env = WitnessBuilderEnv::<Fp, Ff1>::create();
        deserialize_field_element(
            &mut dummy_env,
            [
                BigUint::from(limb0),
                BigUint::from(limb1),
                BigUint::from(limb2),
            ],
        );

        // Check limb are copied into the environment
        let limbs_to_assert = [limb0, limb1, limb2];
        for (i, limb) in limbs_to_assert.iter().enumerate() {
            assert_eq!(
                Fp::from(*limb),
                dummy_env.read_column_direct(SerializationColumn::ChalKimchi(i))
            );
        }

        // Check intermediate limbs
        {
            let bits = Fp::from(limb2).to_bits();
            for j in 0..N_INTERMEDIATE_LIMBS {
                let le_bits: &[bool] = &bits
                    .clone()
                    .into_iter()
                    .skip(j * 4)
                    .take(4)
                    .collect::<Vec<bool>>();
                let t = Fp::from_bits(le_bits).unwrap();
                let intermediate_v =
                    dummy_env.read_column_direct(SerializationColumn::ChalIntermediate(j));
                assert_eq!(
                    t,
                    intermediate_v,
                    "{}",
                    format_args!(
                        "Intermediate limb {j}. Exp value is {:?}, computed is {:?}",
                        t.to_biguint(),
                        intermediate_v.to_biguint()
                    )
                )
            }
        }

        // Checking msm limbs
        for i in 0..N_LIMBS {
            let le_bits: &[bool] = &bits
                .clone()
                .into_iter()
                .skip(i * LIMB_BITSIZE)
                .take(LIMB_BITSIZE)
                .collect::<Vec<bool>>();
            let t = Fp::from_bits(le_bits).unwrap();
            let converted_v = dummy_env.read_column_direct(SerializationColumn::ChalConverted(i));
            assert_eq!(
                t,
                converted_v,
                "{}",
                format_args!(
                    "MSM limb {i}. Exp value is {:?}, computed is {:?}",
                    t.to_biguint(),
                    converted_v.to_biguint()
                )
            )
        }
    }

    #[test]
    fn test_decomposition_zero() {
        test_decomposition_generic(Fp::zero());
    }

    #[test]
    fn test_decomposition_one() {
        test_decomposition_generic(Fp::one());
    }

    #[test]
    fn test_decomposition_random_first_limb_only() {
        let mut rng = make_test_rng();
        let x = rng.gen_range(0..2u128.pow(88) - 1);
        test_decomposition_generic(Fp::from(x));
    }

    #[test]
    fn test_decomposition_second_limb_only() {
        test_decomposition_generic(Fp::from(2u128.pow(88)));
        test_decomposition_generic(Fp::from(2u128.pow(88) + 1));
        test_decomposition_generic(Fp::from(2u128.pow(88) + 2));
        test_decomposition_generic(Fp::from(2u128.pow(88) + 16));
        test_decomposition_generic(Fp::from(2u128.pow(88) + 23234));
    }

    #[test]
    fn test_decomposition_random_second_limb_only() {
        let mut rng = make_test_rng();
        let x = rng.gen_range(0..2u128.pow(88) - 1);
        test_decomposition_generic(Fp::from(2u128.pow(88) + x));
    }

    #[test]
    fn test_decomposition_random() {
        let mut rng = make_test_rng();
        test_decomposition_generic(Fp::rand(&mut rng));
    }

    #[test]
    fn test_decomposition_order_minus_one() {
        let x = BigUint::from_bytes_be(&<Fp as PrimeField>::Params::MODULUS.to_bytes_be())
            - BigUint::from_str("1").unwrap();

        test_decomposition_generic(Fp::from(x));
    }
}
