# frozen_string_literal: true

require_relative "spec_helper"

describe Wid::Parser do
  def parse(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token
    end

    parser = Wid::Parser.new(tokens)
    program = parser.parse

    _(program).must_be_instance_of Wid::AST::Node::Program

    program.to_h[:statements]
  end

  it "parses numbers" do
    [3, 2.5].each do |number|
      _(parse(number.to_s)).must_equal [{type: Wid::AST::Expr::NumberLiteral, value: number}]
    end
  end

  it "parses strings" do
    [%('a string 1'), %("a string 2")].each do |string|
      _(parse(string)).must_equal [{
        type: Wid::AST::Expr::StringLiteral,
        value: string[1...-1]
      }]
    end
  end

  it "parses booleans" do
    %w[true false].each do |string|
      _(parse(string)).must_equal [{
        type: Wid::AST::Expr::BoolLiteral,
        value: string.to_s == "true"
      }]
    end
  end

  it "parses nil" do
    _(parse("nil")).must_equal [{
      type: Wid::AST::Expr::NilLiteral,
      value: nil
    }]
  end

  it "parses equality" do
    _(parse("1 == 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :==,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("1 != 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :!=,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]
  end

  it "parses comparison" do
    _(parse("1 > 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :>,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("1 >= 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :>=,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("1 < 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :<,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("1 <= 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :<=,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]
  end

  it "parses term" do
    _(parse("1 + 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :+,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("1 - 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :-,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]
  end

  it "parses factor" do
    _(parse("1 * 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :*,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("1 / 2")).must_equal [{
      type: Wid::AST::Expr::Binary,
      left: {type: Wid::AST::Expr::NumberLiteral, value: 1},
      operator: :/,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]
  end

  it "parses unary" do
    _(parse("-2")).must_equal [{
      type: Wid::AST::Expr::Unary,
      operator: :-,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("!2")).must_equal [{
      type: Wid::AST::Expr::Unary,
      operator: :!,
      right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]

    _(parse("!!2")).must_equal [{
      type: Wid::AST::Expr::Unary,
      operator: :!,
      right: {
        type: Wid::AST::Expr::Unary,
        operator: :!,
        right: {type: Wid::AST::Expr::NumberLiteral, value: 2}
      }
    }]
  end

  it "parses grouping" do
    _(parse("(2)")).must_equal [{
      type: Wid::AST::Expr::Grouping,
      expr: {type: Wid::AST::Expr::NumberLiteral, value: 2}
    }]
  end
end
