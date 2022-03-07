use crate::circuits::polynomials::generic::testing::{create_circuit, fill_in_witness};
use crate::circuits::{gate::CircuitGate, wires::COLUMNS};
use crate::index::testing::new_index_for_test;
use crate::prover::ProverProof;
use ark_ff::{UniformRand, Zero};
use ark_poly::{univariate::DensePolynomial, UVPolynomial};
use array_init::array_init;
use commitment_dlog::commitment::{b_poly_coefficients, ceil_log2, CommitmentCurve};
use groupmap::GroupMap;
use mina_curves::pasta::{
    fp::Fp,
    vesta::{Affine, VestaParameters},
};
use oracle::{
    poseidon::PlonkSpongeConstants15W,
    sponge::{DefaultFqSponge, DefaultFrSponge},
};
use rand::{rngs::StdRng, SeedableRng};

// aliases

type SpongeParams = PlonkSpongeConstants15W;
type BaseSponge = DefaultFqSponge<VestaParameters, SpongeParams>;
type ScalarSponge = DefaultFrSponge<Fp, SpongeParams>;

#[test]
fn test_generic_gate() {
    let gates = create_circuit(0);

    // create witness
    let mut witness: [Vec<Fp>; COLUMNS] = array_init(|_| vec![Fp::zero(); gates.len()]);
    fill_in_witness(0, &mut witness);

    // create and verify proof based on the witness
    verify_proof(gates, witness, 0);
}

fn verify_proof(gates: Vec<CircuitGate<Fp>>, witness: [Vec<Fp>; COLUMNS], public: usize) {
    // set up
    let rng = &mut StdRng::from_seed([0u8; 32]);
    let group_map = <Affine as CommitmentCurve>::Map::setup();

    // create the index
    let index = new_index_for_test(gates, public);

    // verify the circuit satisfiability by the computed witness
    index.cs.verify(&witness).unwrap();

    // previous opening for recursion
    let prev = {
        let k = ceil_log2(index.srs.g.len());
        let chals: Vec<_> = (0..k).map(|_| Fp::rand(rng)).collect();
        let comm = {
            let coeffs = b_poly_coefficients(&chals);
            let b = DensePolynomial::from_coefficients_vec(coeffs);
            index.srs.commit_non_hiding(&b, None)
        };
        (chals, comm)
    };

    // add the proof to the batch
    let mut batch = Vec::new();
    batch.push(
        ProverProof::create::<BaseSponge, ScalarSponge>(&group_map, witness, &index, vec![prev])
            .unwrap(),
    );

    // verify the proof
    let verifier_index = index.verifier_index();
    let lgr_comms = vec![]; // why empty?
    let batch: Vec<_> = batch
        .iter()
        .map(|proof| (&verifier_index, &lgr_comms, proof))
        .collect();
    ProverProof::verify::<BaseSponge, ScalarSponge>(&group_map, &batch).unwrap();
}